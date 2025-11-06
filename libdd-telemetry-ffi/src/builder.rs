// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ffi::slice::AsBytes;
use libdd_common::Endpoint;
use libdd_common_ffi as ffi;
use libdd_telemetry::{
    data,
    worker::{TelemetryWorkerBuilder, TelemetryWorkerFlavor, TelemetryWorkerHandle},
};
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

use crate::try_c;

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
    let mut builder = TelemetryWorkerBuilder::new_fetch_host(
        service_name.to_utf8_lossy().into_owned(),
        language_name.to_utf8_lossy().into_owned(),
        language_version.to_utf8_lossy().into_owned(),
        tracer_version.to_utf8_lossy().into_owned(),
    );
    // This is not great but maintains compatibility code remove in Builder::run
    builder.config = libdd_telemetry::config::Config::from_env();

    let new = Box::new(builder);
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
    let mut builder = TelemetryWorkerBuilder::new(
        hostname.to_utf8_lossy().into_owned(),
        service_name.to_utf8_lossy().into_owned(),
        language_name.to_utf8_lossy().into_owned(),
        language_version.to_utf8_lossy().into_owned(),
        tracer_version.to_utf8_lossy().into_owned(),
    );
    // This is not great but maintains compatibility code remove in Builder::run
    builder.config = libdd_telemetry::config::Config::from_env();

    let new = Box::new(builder);
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
    config_id: ffi::CharSlice,
) -> MaybeError {
    let name = name.to_utf8_lossy().into_owned();
    let value = value.to_utf8_lossy().into_owned();
    let config_id = if config_id.is_empty() {
        None
    } else {
        Some(config_id.to_utf8_lossy().into_owned())
    };
    builder.configurations.insert(data::Configuration {
        name,
        value,
        origin,
        config_id,
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
        .write(Box::new(crate::try_c!(builder.run())));
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
    mut builder: Box<TelemetryWorkerBuilder>,
    out_handle: NonNull<Box<TelemetryWorkerHandle>>,
) -> MaybeError {
    builder.flavor = TelemetryWorkerFlavor::MetricsLogs;
    out_handle
        .as_ptr()
        .write(Box::new(crate::try_c!(builder.run())));
    MaybeError::None
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_telemetry_builder_with_endpoint_config_endpoint(
    telemetry_builder: &mut TelemetryWorkerBuilder,
    endpoint: &Endpoint,
) -> ffi::MaybeError {
    try_c!(telemetry_builder.config.set_endpoint(endpoint.clone()));
    ffi::MaybeError::None
}
#[repr(C)]
#[allow(dead_code)]
pub enum TelemetryWorkerBuilderEndpointProperty {
    ConfigEndpoint,
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
/// Sets a property from it's string value.
///
/// Available properties:
///
/// * config.endpoint
pub unsafe extern "C" fn ddog_telemetry_builder_with_property_endpoint(
    telemetry_builder: &mut TelemetryWorkerBuilder,
    _property: TelemetryWorkerBuilderEndpointProperty,
    endpoint: &Endpoint,
) -> ffi::MaybeError {
    try_c!(telemetry_builder.config.set_endpoint(endpoint.clone()));
    ffi::MaybeError::None
}
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
/// Sets a property from it's string value.
///
/// Available properties:
///
/// * config.endpoint
pub unsafe extern "C" fn ddog_telemetry_builder_with_endpoint_named_property(
    telemetry_builder: &mut TelemetryWorkerBuilder,
    property: ffi::CharSlice,
    endpoint: &Endpoint,
) -> ffi::MaybeError {
    let property = try_c!(property.try_to_utf8());

    match property {
        "config . endpoint" => {
            try_c!(telemetry_builder.config.set_endpoint(endpoint.clone()));
        }
        _ => return ffi::MaybeError::None,
    }
    ffi::MaybeError::None
}
