// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ffi::slice::AsBytes;
use ffi::MaybeError;
use function_name::named;
use libdd_common::tag::Tag;
use libdd_common_ffi as ffi;
use libdd_telemetry::{
    data::metrics::{MetricNamespace, MetricType},
    metrics::ContextKey,
    worker::TelemetryWorkerHandle,
};

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_telemetry_handle_add_dependency(
    handle: &TelemetryWorkerHandle,
    dependency_name: ffi::CharSlice,
    dependency_version: ffi::CharSlice,
) -> MaybeError {
    let name = dependency_name.to_utf8_lossy().into_owned();
    let version = dependency_version
        .is_empty()
        .then(|| dependency_version.to_utf8_lossy().into_owned());
    crate::try_c!(handle.add_dependency(name, version));
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_telemetry_handle_add_integration(
    handle: &TelemetryWorkerHandle,
    dependency_name: ffi::CharSlice,
    dependency_version: ffi::CharSlice,
    enabled: bool,
    compatible: ffi::Option<bool>,
    auto_enabled: ffi::Option<bool>,
) -> MaybeError {
    let name = dependency_name.to_utf8_lossy().into_owned();
    let version = dependency_version
        .is_empty()
        .then(|| dependency_version.to_utf8_lossy().into_owned());
    crate::try_c!(handle.add_integration(
        name,
        enabled,
        version,
        compatible.into(),
        auto_enabled.into(),
    ));
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
#[named]
/// * indentifier: identifies a logging location uniquely. This can for instance be the template
///   using for the log message or the concatenated file + line of the origin of the log
/// * stack_trace: stack trace associated with the log. If no stack trace is available, an empty
///   string should be passed
pub unsafe extern "C" fn ddog_telemetry_handle_add_log(
    handle: &TelemetryWorkerHandle,
    indentifier: ffi::CharSlice,
    message: ffi::CharSlice,
    level: libdd_telemetry::data::LogLevel,
    stack_trace: ffi::CharSlice,
) -> MaybeError {
    let id = crate::try_c!(indentifier.try_as_bytes().map_err(|e| {
        let func = function_name!();
        format!("{func} failed: identifier failed to convert to a byte slice: {e}")
    }));
    crate::try_c!(handle.add_log(
        id,
        message.to_utf8_lossy().into_owned(),
        level,
        stack_trace
            .is_empty()
            .then(|| stack_trace.to_utf8_lossy().into_owned()),
    ));
    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_telemetry_handle_start(handle: &TelemetryWorkerHandle) -> MaybeError {
    crate::try_c!(handle.send_start());
    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_telemetry_handle_clone(
    handle: &TelemetryWorkerHandle,
) -> Box<TelemetryWorkerHandle> {
    Box::new(handle.clone())
}

#[no_mangle]
pub extern "C" fn ddog_telemetry_handle_stop(handle: &TelemetryWorkerHandle) -> MaybeError {
    crate::try_c!(handle.send_stop());
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
/// * compatible: should be false if the metric is language specific, true otherwise
pub unsafe extern "C" fn ddog_telemetry_handle_register_metric_context(
    handle: &TelemetryWorkerHandle,
    name: ffi::CharSlice,
    metric_type: MetricType,
    tags: ffi::Vec<Tag>,
    common: bool,
    namespace: MetricNamespace,
) -> ContextKey {
    handle.register_metric_context(
        name.to_utf8_lossy().into_owned(),
        tags.into(),
        metric_type,
        common,
        namespace,
    )
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_telemetry_handle_add_point(
    handle: &TelemetryWorkerHandle,
    context_key: &ContextKey,
    value: f64,
) -> MaybeError {
    crate::try_c!(handle.add_point(value, context_key, Vec::new()));
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_telemetry_handle_add_point_with_tags(
    handle: &TelemetryWorkerHandle,
    context_key: &ContextKey,
    value: f64,
    extra_tags: ffi::Vec<Tag>,
) -> MaybeError {
    crate::try_c!(handle.add_point(value, context_key, extra_tags.into()));
    MaybeError::None
}

#[no_mangle]
/// This function takes ownership of the handle. It should not be used after calling it
pub extern "C" fn ddog_telemetry_handle_wait_for_shutdown(handle: Box<TelemetryWorkerHandle>) {
    handle.wait_for_shutdown()
}

#[no_mangle]
/// This function takes ownership of the handle. It should not be used after calling it
pub extern "C" fn ddog_telemetry_handle_wait_for_shutdown_ms(
    handle: Box<TelemetryWorkerHandle>,
    wait_for_ms: u64,
) {
    handle.wait_for_shutdown_deadline(
        std::time::Instant::now() + std::time::Duration::from_millis(wait_for_ms),
    )
}

#[no_mangle]
/// Drops the handle without waiting for shutdown. The worker will continue running in the
/// background until it exits by itself
pub extern "C" fn ddog_telemetry_handle_drop(handle: Box<TelemetryWorkerHandle>) {
    drop(handle);
}
