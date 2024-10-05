// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use data_pipeline::trace_exporter::{
    ResponseCallback, TraceExporter, TraceExporterInputFormat, TraceExporterOutputFormat,
};
use ddcommon_ffi::{
    slice::{AsBytes, ByteSlice},
    CharSlice, MaybeError,
};
use std::{ffi::c_char, ptr::NonNull, time::Duration};

#[allow(dead_code)]
#[repr(C)]
pub enum TraceExporterConfigOption<'a> {
    Url(CharSlice<'a>),
    TracerVersion(CharSlice<'a>),
    Language(CharSlice<'a>),
    LanguageVersion(CharSlice<'a>),
    LanguageInterpreter(CharSlice<'a>),
    InputFormat(TraceExporterInputFormat),
    OutputFormat(TraceExporterOutputFormat),
    ResponseCallback(extern "C" fn(*const c_char)),
    Hostname(CharSlice<'a>),
    Env(CharSlice<'a>),
    Version(CharSlice<'a>),
    Service(CharSlice<'a>),
    ComputeStats(bool),
}

#[derive(Default)]
pub struct TraceExporterConfig<'a> {
    url: Option<CharSlice<'a>>,
    tracer_version: Option<CharSlice<'a>>,
    language: Option<CharSlice<'a>>,
    language_version: Option<CharSlice<'a>>,
    language_interpreter: Option<CharSlice<'a>>,
    input_format: TraceExporterInputFormat,
    output_format: TraceExporterOutputFormat,
    agent_response_callback: Option<extern "C" fn(*const c_char)>,
    compute_stats: bool,
    service: Option<CharSlice<'a>>,
    env: Option<CharSlice<'a>>,
    hostname: Option<CharSlice<'a>>,
    version: Option<CharSlice<'a>>,
}

/// Create a new TraceExporterConfig instance.
///
/// # Arguments
///
/// * `out_handle` - The handle to write the TraceExporterConfig instance in.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_new(
    out_handle: NonNull<Box<TraceExporterConfig>>,
) -> MaybeError {
    out_handle
        .as_ptr()
        .write(Box::<TraceExporterConfig<'_>>::default());
    MaybeError::None
}

/// Set properties on a TraceExporterConfig instance.
///
/// # Arguments
///
/// * `option` - [TraceExporterConfig] enum member.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_option<'a>(
    handle: &mut TraceExporterConfig<'a>,
    option: TraceExporterConfigOption<'a>,
) -> MaybeError {
    match option {
        TraceExporterConfigOption::Url(url) => handle.url = Some(url),
        TraceExporterConfigOption::TracerVersion(version) => handle.tracer_version = Some(version),
        TraceExporterConfigOption::Language(lang) => handle.language = Some(lang),
        TraceExporterConfigOption::LanguageVersion(lang_version) => {
            handle.language_version = Some(lang_version)
        }
        TraceExporterConfigOption::LanguageInterpreter(interp) => {
            handle.language_interpreter = Some(interp)
        }
        TraceExporterConfigOption::InputFormat(input) => handle.input_format = input,
        TraceExporterConfigOption::OutputFormat(output) => handle.output_format = output,
        TraceExporterConfigOption::ResponseCallback(cback) => {
            handle.agent_response_callback = Some(cback)
        }
        TraceExporterConfigOption::Env(env) => handle.env = Some(env),
        TraceExporterConfigOption::Service(service) => handle.service = Some(service),
        TraceExporterConfigOption::Hostname(hostname) => handle.hostname = Some(hostname),
        TraceExporterConfigOption::Version(version) => handle.version = Some(version),
        TraceExporterConfigOption::ComputeStats(compute) => handle.compute_stats = compute,
    }
    MaybeError::None
}


/// Create a new TraceExporter instance.
///
/// # Arguments
///
/// * `out_handle` - The handle to write the TraceExporter instance in.
/// * `config` - Configuration handle to set TraceExporter properties. The handle will be owned by
///   the function and will be properly freed after creating the exporter instance.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_new(
    out_handle: NonNull<Box<TraceExporter>>,
    config: Box<TraceExporterConfig>,
) -> MaybeError {
    // TODO - handle errors - https://datadoghq.atlassian.net/browse/APMSP-1095
    let mut builder = TraceExporter::builder();
    if let Some(url) = config.url {
        builder = builder.set_url(url.to_utf8_lossy().as_ref());
    }
    if let Some(tracer_version) = config.tracer_version {
        builder = builder.set_tracer_version(tracer_version.to_utf8_lossy().as_ref());
    }
    if let Some(language) = config.language {
        builder = builder.set_language(language.to_utf8_lossy().as_ref());
    }
    if let Some(lang_version) = config.language_version {
        builder = builder.set_language_version(lang_version.to_utf8_lossy().as_ref());
    }
    if let Some(interpreter) = config.language_interpreter {
        builder = builder.set_language_interpreter(interpreter.to_utf8_lossy().as_ref());
    }
    if let Some(callback) = config.agent_response_callback {
        let wrapper = ResponseCallbackWrapper {
            response_callback: callback,
        };
        builder = builder.set_response_callback(Box::new(wrapper));
    }
    if let Some(hostname) = config.hostname {
        builder = builder.set_hostname(hostname.to_utf8_lossy().as_ref());
    }
    if let Some(env) = config.env {
        builder = builder.set_env(env.to_utf8_lossy().as_ref());
    }
    if let Some(service) = config.service {
        builder = builder.set_service(service.to_utf8_lossy().as_ref());
    }
    if config.compute_stats {
        builder = builder.enable_stats(Duration::from_secs(10));
        // TODO: APMSP-1317 Enable peer tags aggregation and stats by span_kind based on agent
        // configuration
    }


    let exporter = builder
        .set_input_format(config.input_format)
        .set_output_format(config.output_format)
        .build()
        .unwrap();

    out_handle.as_ptr().write(Box::new(exporter));
    MaybeError::None
}

struct ResponseCallbackWrapper {
    response_callback: extern "C" fn(*const c_char),
}

impl ResponseCallback for ResponseCallbackWrapper {
    fn call(&self, response: &str) {
        let c_response = std::ffi::CString::new(response).unwrap();
        (self.response_callback)(c_response.as_ptr());
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
///   TraceExporter.
/// * `trace_count` - The number of traces to send to the Datadog Agent.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_send(
    handle: &TraceExporter,
    trace: ByteSlice,
    trace_count: usize,
) -> MaybeError {
    // TODO - handle errors - https://datadoghq.atlassian.net/browse/APMSP-1095
    handle
        .send(trace.as_bytes(), trace_count)
        .unwrap_or(String::from(""));
    MaybeError::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn config_constructor_test() {
        let mut config: MaybeUninit<Box<TraceExporterConfig>> = MaybeUninit::uninit();

        unsafe {
            let ret = ddog_trace_exporter_config_new(NonNull::new_unchecked(&mut config).cast());
            assert_eq!(MaybeError::None, ret);

            let config = config.assume_init();
            assert_eq!(config.url, None);
            assert_eq!(config.input_format, TraceExporterInputFormat::V04);
            assert_eq!(config.language, None);
            assert_eq!(config.output_format, TraceExporterOutputFormat::V04);
            assert_eq!(config.tracer_version, None);
            assert_eq!(config.language_version, None);
            assert_eq!(config.language_interpreter, None);
            assert_eq!(config.agent_response_callback, None);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn config_set_option_test() {
        let mut config: MaybeUninit<Box<TraceExporterConfig>> = MaybeUninit::uninit();

        unsafe {
            let ret = ddog_trace_exporter_config_new(NonNull::new_unchecked(&mut config).cast());
            assert_eq!(MaybeError::None, ret);

            let mut config = config.assume_init();
            ddog_trace_exporter_config_set_option(
                config.as_mut(),
                TraceExporterConfigOption::Url(CharSlice::from("http://localhost")),
            );
            ddog_trace_exporter_config_set_option(
                config.as_mut(),
                TraceExporterConfigOption::InputFormat(TraceExporterInputFormat::V04),
            );
            ddog_trace_exporter_config_set_option(
                config.as_mut(),
                TraceExporterConfigOption::OutputFormat(TraceExporterOutputFormat::V04),
            );
            ddog_trace_exporter_config_set_option(
                config.as_mut(),
                TraceExporterConfigOption::Language(CharSlice::from("foo")),
            );
            ddog_trace_exporter_config_set_option(
                config.as_mut(),
                TraceExporterConfigOption::LanguageVersion(CharSlice::from("foo")),
            );
            ddog_trace_exporter_config_set_option(
                config.as_mut(),
                TraceExporterConfigOption::TracerVersion(CharSlice::from("foo")),
            );
            ddog_trace_exporter_config_set_option(
                config.as_mut(),
                TraceExporterConfigOption::LanguageInterpreter(CharSlice::from("foo")),
            );

            assert_eq!(config.url, Some(CharSlice::from("http://localhost")));
            assert_eq!(config.language, Some(CharSlice::from("foo")));
            assert_eq!(config.language_version, Some(CharSlice::from("foo")));
            assert_eq!(config.language_interpreter, Some(CharSlice::from("foo")));
            assert_eq!(config.tracer_version, Some(CharSlice::from("foo")));
            assert_eq!(config.input_format, TraceExporterInputFormat::V04);
            assert_eq!(config.output_format, TraceExporterOutputFormat::V04);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn exporter_constructor_test() {
        let mut config: MaybeUninit<Box<TraceExporterConfig>> = MaybeUninit::uninit();
        let mut exporter: MaybeUninit<Box<TraceExporter>> = MaybeUninit::uninit();

        unsafe {
            let _ = ddog_trace_exporter_config_new(NonNull::new_unchecked(&mut config).cast());
            let mut config = config.assume_init();

            ddog_trace_exporter_config_set_option(
                config.as_mut(),
                TraceExporterConfigOption::Url(CharSlice::from("http://localhost")),
            );
            ddog_trace_exporter_config_set_option(
                config.as_mut(),
                TraceExporterConfigOption::InputFormat(TraceExporterInputFormat::V04),
            );
            ddog_trace_exporter_config_set_option(
                config.as_mut(),
                TraceExporterConfigOption::OutputFormat(TraceExporterOutputFormat::V04),
            );
            ddog_trace_exporter_config_set_option(
                config.as_mut(),
                TraceExporterConfigOption::Language(CharSlice::from("foo")),
            );
            ddog_trace_exporter_config_set_option(
                config.as_mut(),
                TraceExporterConfigOption::LanguageVersion(CharSlice::from("foo")),
            );
            ddog_trace_exporter_config_set_option(
                config.as_mut(),
                TraceExporterConfigOption::TracerVersion(CharSlice::from("foo")),
            );
            ddog_trace_exporter_config_set_option(
                config.as_mut(),
                TraceExporterConfigOption::LanguageInterpreter(CharSlice::from("foo")),
            );

            let ret = ddog_trace_exporter_new(NonNull::new_unchecked(&mut exporter).cast(), config);
            println!("After new");
            assert_eq!(ret, MaybeError::None);
        }
    }
}
