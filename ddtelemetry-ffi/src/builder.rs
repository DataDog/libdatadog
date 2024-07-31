// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon_ffi as ffi;
use ddtelemetry::{
    data,
    worker::{TelemetryWorkerBuilder, TelemetryWorkerHandle},
};
use ffi::slice::AsBytes;
use std::ptr::NonNull;

use ffi::MaybeError;

#[cfg(not(feature = "expanded_builder_macros"))]
mod macros;
#[cfg(not(feature = "expanded_builder_macros"))]
pub use macros::*;

#[cfg(feature = "expanded_builder_macros")]
mod expanded;
#[cfg(feature = "expanded_builder_macros")]
pub use expanded::*;

/// # Safety
/// * builder should be a non null pointer to a null pointer to a builder
#[no_mangle]
pub unsafe extern "C" fn ddog_telemetry_builder_instantiate(
    out_builder: NonNull<Box<TelemetryWorkerBuilder>>,
    service_name: ffi::CharSlice,
    language_name: ffi::CharSlice,
    language_version: ffi::CharSlice,
    tracer_version: ffi::CharSlice,
) -> MaybeError {
    let new = Box::new(TelemetryWorkerBuilder::new_fetch_host(
        service_name.to_utf8_lossy().into_owned(),
        language_name.to_utf8_lossy().into_owned(),
        language_version.to_utf8_lossy().into_owned(),
        tracer_version.to_utf8_lossy().into_owned(),
    ));
    out_builder.as_ptr().write(new);
    MaybeError::None
}

/// # Safety
/// * builder should be a non null pointer to a null pointer to a builder
#[no_mangle]
pub unsafe extern "C" fn ddog_telemetry_builder_instantiate_with_hostname(
    out_builder: NonNull<Box<TelemetryWorkerBuilder>>,
    hostname: ffi::CharSlice,
    service_name: ffi::CharSlice,
    language_name: ffi::CharSlice,
    language_version: ffi::CharSlice,
    tracer_version: ffi::CharSlice,
) -> MaybeError {
    let new = Box::new(TelemetryWorkerBuilder::new(
        hostname.to_utf8_lossy().into_owned(),
        service_name.to_utf8_lossy().into_owned(),
        language_name.to_utf8_lossy().into_owned(),
        language_version.to_utf8_lossy().into_owned(),
        tracer_version.to_utf8_lossy().into_owned(),
    ));

    out_builder.as_ptr().write(new);
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_telemetry_builder_with_native_deps(
    builder: &mut TelemetryWorkerBuilder,
    include_native_deps: bool,
) -> MaybeError {
    builder.native_deps = include_native_deps;
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_telemetry_builder_with_rust_shared_lib_deps(
    builder: &mut TelemetryWorkerBuilder,
    include_rust_shared_lib_deps: bool,
) -> MaybeError {
    builder.rust_shared_lib_deps = include_rust_shared_lib_deps;
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_telemetry_builder_with_config(
    builder: &mut TelemetryWorkerBuilder,
    name: ffi::CharSlice,
    value: ffi::CharSlice,
    origin: data::ConfigurationOrigin,
) -> MaybeError {
    let name = name.to_utf8_lossy().into_owned();
    let value = value.to_utf8_lossy().into_owned();
    builder.configurations.insert(data::Configuration {
        name,
        value,
        origin,
    });
    MaybeError::None
}

#[no_mangle]
/// Builds the telemetry worker and return a handle to it
///
/// # Safety
/// * handle should be a non null pointer to a null pointer
pub unsafe extern "C" fn ddog_telemetry_builder_run(
    builder: Box<TelemetryWorkerBuilder>,
    out_handle: NonNull<Box<TelemetryWorkerHandle>>,
) -> MaybeError {
    out_handle
        .as_ptr()
        .write(Box::new(ffi::try_c!(builder.run())));
    MaybeError::None
}

#[no_mangle]
/// Builds the telemetry worker and return a handle to it. The worker will only process and send
/// telemetry metrics and telemetry logs. Any lifecyle/dependency/configuration event will be
/// ignored
///
/// # Safety
/// * handle should be a non null pointer to a null pointer
pub unsafe extern "C" fn ddog_telemetry_builder_run_metric_logs(
    builder: Box<TelemetryWorkerBuilder>,
    out_handle: NonNull<Box<TelemetryWorkerHandle>>,
) -> MaybeError {
    out_handle
        .as_ptr()
        .write(Box::new(ffi::try_c!(builder.run_metrics_logs())));
    MaybeError::None
}
