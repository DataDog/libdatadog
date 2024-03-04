// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
use ddcommon_ffi as ffi;
use ddtelemetry::{
    data,
    worker::{TelemetryWorkerBuilder, TelemetryWorkerHandle},
};
use ffi::slice::AsBytes;

use crate::MaybeError;

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
pub unsafe extern "C" fn ddog_builder_instantiate(
    builder: &mut *mut TelemetryWorkerBuilder,
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
    // Leaking is the last thing we do before returning
    // Otherwise we would need to manually drop it in case of error
    *builder = Box::into_raw(new);
    MaybeError::None
}

/// # Safety
/// * builder should be a non null pointer to a null pointer to a builder
#[no_mangle]
pub unsafe extern "C" fn ddog_builder_instantiate_with_hostname(
    builder: &mut *mut TelemetryWorkerBuilder,
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

    // Leaking is the last thing we do before returning
    // Otherwise we would need to manually drop it in case of error
    *builder = Box::into_raw(new);
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_builder_with_native_deps(
    builder: &mut TelemetryWorkerBuilder,
    include_native_deps: bool,
) -> MaybeError {
    builder.native_deps = include_native_deps;
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_builder_with_rust_shared_lib_deps(
    builder: &mut TelemetryWorkerBuilder,
    include_rust_shared_lib_deps: bool,
) -> MaybeError {
    builder.rust_shared_lib_deps = include_rust_shared_lib_deps;
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_builder_with_config(
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
/// # Safety
/// * handle should be a non null pointer to a null pointer
pub unsafe extern "C" fn ddog_builder_run(
    builder: Box<TelemetryWorkerBuilder>,
    handle: &mut *mut TelemetryWorkerHandle,
) -> MaybeError {
    *handle = Box::into_raw(Box::new(crate::try_c!(builder.run())));
    MaybeError::None
}
